use crate::canvs::{CanvsSnapshot, CanvsTask, CanvsTaskList};

pub fn task_list_page(workspace_name: &str, list: &CanvsTaskList) -> String {
    let total_count = list.tasks.len();
    let mut active_count = 0;
    let mut verifying_count = 0;
    let mut completed_count = 0;
    let mut worktree_count = 0;

    for task in &list.tasks {
        if task.active || task.status == "active" {
            active_count += 1;
        }
        if task.status == "verifying" {
            verifying_count += 1;
        }
        if task.status == "completed" || task.status == "completed_unverified" {
            completed_count += 1;
        }
        if task.workspace_mode == "worktree" {
            worktree_count += 1;
        }
    }

    let cards = if list.tasks.is_empty() {
        "<section class='empty-card'>
            <div class='empty-icon'>📋</div>
            <h2>暂无任务</h2>
            <p>该工作区还没有 Harness 当前任务或历史任务记录。</p>
        </section>"
            .to_string()
    } else {
        list.tasks
            .iter()
            .map(task_card)
            .collect::<Vec<_>>()
            .join("")
    };

    let body = format!(
        "<header class='hero'>
            <div class='hero-main'>
                <div class='brand-badge'>
                    <span class='pulse-dot'></span>
                    <span>ANCHOR CANVS</span>
                </div>
                <h1>{}</h1>
                <p class='subtitle'>当前任务（可多项）与历史任务按更新时间倒序排列；默认任务单独标记。每个入口只读取当前工作区的 Harness 数据。</p>
            </div>
            <div class='hero-stats'>
                <div class='stat-pill'>
                    <span class='stat-label'>总任务</span>
                    <strong class='stat-val'>{}</strong>
                </div>
                <div class='stat-pill'>
                    <span class='stat-label'>进行中</span>
                    <strong class='stat-val primary'>{}</strong>
                </div>
                <div class='stat-pill'>
                    <span class='stat-label'>验证中</span>
                    <strong class='stat-val warning'>{}</strong>
                </div>
                <div class='stat-pill'>
                    <span class='stat-label'>已完成</span>
                    <strong class='stat-val success'>{}</strong>
                </div>
                <div class='hero-time'>
                    <span>最后刷新: </span>
                    <time data-time='{}'>{}</time>
                </div>
            </div>
        </header>

        <section class='toolbar'>
            <div class='search-box'>
                <svg class='search-icon' viewBox='0 0 24 24' width='16' height='16' stroke='currentColor' stroke-width='2' fill='none' stroke-linecap='round' stroke-linejoin='round'><circle cx='11' cy='11' r='8'></circle><line x1='21' y1='21' x2='16.65' y2='16.65'></line></svg>
                <input type='text' id='task-search' placeholder='搜索任务目标、ID、分支或 Commit...' autocomplete='off' />
                <button type='button' id='clear-search' class='clear-btn' style='display:none;' title='清空'>✕</button>
            </div>
            <div class='filter-tabs' id='filter-tabs'>
                <button type='button' class='tab active' data-filter='all'>全部 <span class='tab-count'>{}</span></button>
                <button type='button' class='tab' data-filter='active'>进行中 <span class='tab-count'>{}</span></button>
                <button type='button' class='tab' data-filter='verifying'>验证中 <span class='tab-count'>{}</span></button>
                <button type='button' class='tab' data-filter='completed'>已完成 <span class='tab-count'>{}</span></button>
                <button type='button' class='tab' data-filter='worktree'>Worktree <span class='tab-count'>{}</span></button>
            </div>
        </section>

        <main class='cards' id='task-grid'>
            {cards}
            <div id='no-match' class='empty-card' style='display:none;'>
                <div class='empty-icon'>🔍</div>
                <h2>未找到匹配任务</h2>
                <p>请尝试更换搜索关键词或切换状态过滤条件。</p>
            </div>
        </main>",
        escape_html(workspace_name),
        total_count,
        active_count,
        verifying_count,
        completed_count,
        escape_attr(&list.refreshed_at),
        escape_html(&list.refreshed_at),
        total_count,
        active_count,
        verifying_count,
        completed_count,
        worktree_count,
    );

    page(
        &format!("{} · Canvs 任务看板", escape_html(workspace_name)),
        &body,
        10_000,
        true,
    )
}

fn task_scope_badges(task: &CanvsTask) -> String {
    let mut badges = String::new();
    if task.active {
        badges.push_str("<span class='badge badge-primary'>活动任务</span>");
    }
    if task.current {
        badges.push_str("<span class='badge badge-secondary'>默认任务</span>");
    }
    if task.workspace_mode == "worktree" {
        badges.push_str("<span class='badge badge-purple'>Git Worktree</span>");
    }
    if badges.is_empty() {
        badges.push_str("<span class='badge badge-muted'>历史任务</span>");
    }
    badges
}

fn task_card(task: &CanvsTask) -> String {
    let scope_badges = task_scope_badges(task);
    let total_steps = task.completed_steps.len() + task.pending_steps.len();
    let filter_category = if task.active || task.status == "active" {
        "active"
    } else if task.status == "verifying" {
        "verifying"
    } else if task.status == "completed" || task.status == "completed_unverified" {
        "completed"
    } else if task.status == "failed" || task.status == "incomplete" {
        "failed"
    } else {
        "other"
    };

    let worktree_flag = if task.workspace_mode == "worktree" {
        "true"
    } else {
        "false"
    };
    let search_content = format!(
        "{} {} {} {}",
        task.objective,
        task.id,
        task.branch.as_deref().unwrap_or(""),
        task.expected_head.as_deref().unwrap_or("")
    )
    .to_lowercase();

    format!(
        "<a class='task-card' href='./canvs/tasks/{}' data-status='{}' data-category='{}' data-worktree='{}' data-search='{}'>
            <div class='card-header'>
                <div class='badges'>
                    {scope_badges}
                    <span class='badge {}'>{}</span>
                </div>
                <button type='button' class='copy-id-btn' data-copy='{}' title='点击复制任务 ID' onclick='event.preventDefault();event.stopPropagation();copyText(\"{}\", this);'>
                    <svg viewBox='0 0 24 24' width='12' height='12' stroke='currentColor' stroke-width='2' fill='none'><rect x='9' y='9' width='13' height='13' rx='2' ry='2'></rect><path d='M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1'></path></svg>
                    <code>{}</code>
                </button>
            </div>
            <h2 class='card-title'>{}</h2>
            <div class='progress-section'>
                <div class='progress-bar'>
                    <div class='progress-fill' style='width:{}%'></div>
                </div>
                <div class='progress-meta'>
                    <strong>{}%</strong>
                    <span>{}/{} 步骤</span>
                </div>
            </div>
            <footer class='card-footer'>
                <div class='footer-meta'>
                    <span class='time-text' title='更新时间'>
                        <svg viewBox='0 0 24 24' width='12' height='12' stroke='currentColor' stroke-width='2' fill='none'><circle cx='12' cy='12' r='10'></circle><polyline points='12 6 12 12 16 14'></polyline></svg>
                        <time data-time='{}'>{}</time>
                    </span>
                </div>
                <div class='footer-git'>
                    <span class='git-badge' title='Git 分支与 Commit'>
                        <svg viewBox='0 0 24 24' width='12' height='12' stroke='currentColor' stroke-width='2' fill='none'><line x1='6' y1='3' x2='6' y2='15'></line><circle cx='18' cy='6' r='3'></circle><circle cx='6' cy='18' r='3'></circle><path d='M18 9a9 9 0 0 1-9 9'></path></svg>
                        <span class='branch-name'>{}</span>
                        <span class='commit-sha'>{}</span>
                    </span>
                </div>
            </footer>
        </a>",
        escape_attr(&task.id),
        escape_attr(&task.status),
        filter_category,
        worktree_flag,
        escape_attr(&search_content),
        status_badge_class(&task.status),
        escape_html(&status_label(&task.status)),
        escape_attr(&task.id),
        escape_attr(&task.id),
        escape_html(&short_hash(&task.id)),
        escape_html(&task.objective),
        task.progress_percent,
        task.progress_percent,
        task.completed_steps.len(),
        total_steps,
        escape_attr(task.last_activity_at.as_deref().unwrap_or(&task.updated_at)),
        escape_html(task.last_activity_at.as_deref().unwrap_or(&task.updated_at)),
        escape_html(task.branch.as_deref().unwrap_or("master")),
        escape_html(&short_hash(task.expected_head.as_deref().unwrap_or(""))),
    )
}

pub fn task_detail_page(workspace_name: &str, snapshot: &CanvsSnapshot) -> String {
    let Some(task) = snapshot.task.as_ref() else {
        return error_page(
            workspace_name,
            "任务不存在",
            "没有找到对应的 Harness 任务记录。",
        );
    };

    let completed = string_list(&task.completed_steps, "尚无已完成步骤", true);
    let pending = string_list(&task.pending_steps, "没有待处理步骤", false);
    let total_steps = task.completed_steps.len() + task.pending_steps.len();

    let operations = if snapshot.recent_operations.is_empty() {
        empty_line("当前任务还没有操作记录")
    } else {
        snapshot
            .recent_operations
            .iter()
            .map(|op| {
                format!(
                    "<article class='timeline-row'>
                        <div class='timeline-body'>
                            <div class='timeline-header'>
                                <strong class='op-tool'>{}</strong>
                                <span class='op-kind'>{}</span>
                            </div>
                            <time class='timeline-time' data-time='{}'>{}</time>
                        </div>
                        <div class='timeline-end'>
                            <span class='badge {}'>{}</span>
                            <small class='timeline-meta'>{}</small>
                        </div>
                    </article>",
                    escape_html(&op.tool),
                    escape_html(&op.kind),
                    escape_attr(&op.created_at),
                    escape_html(&op.created_at),
                    outcome_badge_class(op.ok),
                    escape_html(&op.status),
                    operation_meta(op.affected_files, op.duration_ms),
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
            .map(|v| {
                format!(
                    "<article class='timeline-row'>
                        <div class='timeline-body'>
                            <code class='command-code'>{}</code>
                            <div class='v-meta'>
                                <span>{}</span>
                                <span>·</span>
                                <span>{}</span>
                                <span>·</span>
                                <time data-time='{}'>{}</time>
                            </div>
                        </div>
                        <div class='timeline-end'>
                            <span class='badge {}'>{}</span>
                            <small class='timeline-meta'>{}</small>
                        </div>
                    </article>",
                    escape_html(&v.command),
                    escape_html(&v.kind),
                    escape_html(&v.level),
                    escape_attr(&v.created_at),
                    escape_html(&v.created_at),
                    if v.passed {
                        "badge-success"
                    } else {
                        "badge-destructive"
                    },
                    escape_html(&disposition_label(&v.disposition)),
                    verification_meta(v.exit_code, v.duration_ms),
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
            .map(|c| {
                let hash = c.commit_sha.as_deref().unwrap_or(&c.id);
                let files = if c.committed_files.is_empty() {
                    "没有提交文件".to_string()
                } else {
                    c.committed_files
                        .iter()
                        .take(4)
                        .map(|file| escape_html(file))
                        .collect::<Vec<_>>()
                        .join(" · ")
                };
                format!(
                    "<article class='timeline-row'>
                        <div class='timeline-body'>
                            <div class='change-header'>
                                <span class='sha-pill'>{}</span>
                                <time class='timeline-time' data-time='{}'>{}</time>
                            </div>
                            <p class='files-summary'>{}</p>
                        </div>
                        <div class='timeline-end'>
                            <span class='badge badge-secondary'>{} 文件</span>
                            <small class='timeline-meta'>{} 验证</small>
                        </div>
                    </article>",
                    escape_html(&short_hash(hash)),
                    escape_attr(&c.created_at),
                    escape_html(&c.created_at),
                    files,
                    c.committed_files.len(),
                    c.verification_count,
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
            .map(|e| {
                format!(
                    "<article class='timeline-row'>
                        <div class='timeline-body'>
                            <strong class='event-kind'>{}</strong>
                            <div class='event-meta'>
                                <span>{}</span>
                                <span>·</span>
                                <time data-time='{}'>{}</time>
                            </div>
                        </div>
                        <div class='timeline-end'>
                            <span class='badge {}'>{}</span>
                        </div>
                    </article>",
                    escape_html(&event_kind_label(&e.kind)),
                    escape_html(e.tool_name.as_deref().unwrap_or("Harness")),
                    escape_attr(&e.created_at),
                    escape_html(&e.created_at),
                    outcome_badge_class(e.ok),
                    if e.affected_files > 0 {
                        format!("{} 文件", e.affected_files)
                    } else if e.ok == Some(false) {
                        "失败".into()
                    } else {
                        "正常".into()
                    },
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };

    let scope_badges = task_scope_badges(task);
    let body = format!(
        "<nav class='top-nav'>
            <a href='../../canvs' class='nav-back'>
                <svg viewBox='0 0 24 24' width='16' height='16' stroke='currentColor' stroke-width='2' fill='none'><line x1='19' y1='12' x2='5' y2='12'></line><polyline points='12 19 5 12 12 5'></polyline></svg>
                <span>返回任务看板</span>
            </a>
            <div class='nav-context'>
                <span>工作区: </span>
                <strong>{}</strong>
            </div>
        </nav>

        <header class='detail-hero'>
            <div class='detail-header-top'>
                <div class='badges'>
                    {scope_badges}
                    <span class='badge {}'>{}</span>
                </div>
                <button type='button' class='copy-id-btn full' data-copy='{}' onclick='copyText(\"{}\", this);' title='复制完整任务 ID'>
                    <svg viewBox='0 0 24 24' width='14' height='14' stroke='currentColor' stroke-width='2' fill='none'><rect x='9' y='9' width='13' height='13' rx='2' ry='2'></rect><path d='M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1'></path></svg>
                    <code>{}</code>
                </button>
            </div>
            <h1 class='detail-title'>{}</h1>
            <div class='detail-progress-card'>
                <div class='detail-progress-head'>
                    <span class='progress-label'>任务完成进度</span>
                    <span class='progress-numbers'><strong>{}%</strong> · {}/{} 步骤</span>
                </div>
                <div class='progress-bar large'>
                    <div class='progress-fill' style='width:{}%'></div>
                </div>
            </div>
            <div class='detail-meta-strip'>
                <div class='meta-item'>
                    <span class='label'>最近活动</span>
                    <time data-time='{}'>{}</time>
                </div>
                <div class='meta-item'>
                    <span class='label'>更新时间</span>
                    <time data-time='{}'>{}</time>
                </div>
                <div class='meta-item'>
                    <span class='label'>Git 分支</span>
                    <span class='value-pill'>{}</span>
                </div>
                <div class='meta-item'>
                    <span class='label'>当前 HEAD</span>
                    <span class='sha-pill'>{}</span>
                </div>
            </div>
        </header>

        <main class='detail-grid'>
            <section class='panel wide'>
                <div class='panel-header'>
                    <h2>任务步骤分解</h2>
                    <span class='badge badge-secondary'>{} 已完成 / {} 待办</span>
                </div>
                <div class='steps-grid'>
                    <div class='steps-column'>
                        <h3 class='steps-col-title completed'>
                            <span class='dot'></span>已完成步骤
                        </h3>
                        {completed}
                    </div>
                    <div class='steps-column'>
                        <h3 class='steps-col-title pending'>
                            <span class='dot'></span>待处理步骤
                        </h3>
                        {pending}
                    </div>
                </div>
            </section>

            <section class='panel'>
                <div class='panel-header'>
                    <h2>任务基线信息</h2>
                </div>
                <dl class='facts-grid'>
                    <div class='fact-card'>
                        <dt>初始 HEAD</dt>
                        <dd><span class='sha-pill'>{}</span></dd>
                    </div>
                    <div class='fact-card'>
                        <dt>当前预期 HEAD</dt>
                        <dd><span class='sha-pill'>{}</span></dd>
                    </div>
                    <div class='fact-card'>
                        <dt>最新变更 ID</dt>
                        <dd><code>{}</code></dd>
                    </div>
                    <div class='fact-card'>
                        <dt>最新验证 ID</dt>
                        <dd><code>{}</code></dd>
                    </div>
                </dl>
            </section>

            <section class='panel'>
                <div class='panel-header'>
                    <h2>最近工具操作</h2>
                    <span class='panel-count'>{} 项</span>
                </div>
                <div class='panel-body'>
                    {operations}
                </div>
            </section>

            <section class='panel'>
                <div class='panel-header'>
                    <h2>有效验证记录</h2>
                    <span class='panel-count'>{} 项</span>
                </div>
                <div class='panel-body'>
                    {verifications}
                </div>
            </section>

            <section class='panel'>
                <div class='panel-header'>
                    <h2>分段提交记录</h2>
                    <span class='panel-count'>{} 次</span>
                </div>
                <div class='panel-body'>
                    {changes}
                </div>
            </section>

            <section class='panel wide'>
                <div class='panel-header'>
                    <h2>任务生命周期事件</h2>
                    <span class='panel-count'>{} 项</span>
                </div>
                <div class='panel-body'>
                    {events}
                </div>
            </section>
        </main>",
        escape_html(workspace_name),
        status_badge_class(&task.status),
        escape_html(&status_label(&task.status)),
        escape_attr(&task.id),
        escape_attr(&task.id),
        escape_html(&task.id),
        escape_html(&task.objective),
        task.progress_percent,
        task.completed_steps.len(),
        total_steps,
        task.progress_percent,
        escape_attr(task.last_activity_at.as_deref().unwrap_or(&task.updated_at)),
        escape_html(task.last_activity_at.as_deref().unwrap_or(&task.updated_at)),
        escape_attr(&task.updated_at),
        escape_html(&task.updated_at),
        escape_html(task.branch.as_deref().unwrap_or("—")),
        escape_html(&short_hash(task.expected_head.as_deref().unwrap_or(""))),
        task.completed_steps.len(),
        task.pending_steps.len(),
        escape_html(&short_hash(task.head.as_deref().unwrap_or(""))),
        escape_html(&short_hash(task.expected_head.as_deref().unwrap_or(""))),
        escape_html(task.latest_change_id.as_deref().unwrap_or("—")),
        escape_html(task.latest_verification_id.as_deref().unwrap_or("—")),
        snapshot.recent_operations.len(),
        snapshot.verifications.len(),
        snapshot.changes.len(),
        snapshot.recent_events.len(),
    );

    page(
        &format!("{} · Canvs 任务详情", escape_html(&task.objective)),
        &body,
        if task.active { 5_000 } else { 0 },
        false,
    )
}

pub fn unauthorized_page(workspace_name: &str) -> String {
    page(
        "Canvs 需要认证",
        &format!(
            "<main class='center-card'>
                <div class='brand-badge' style='margin-bottom:12px;'>
                    <span>ANCHOR CANVS</span>
                </div>
                <h1>需要认证</h1>
                <p>请输入工作区“<strong>{}</strong>”的 MCP 授权口令。Bearer 模式下请输入 Bearer Token。</p>
                <p class='muted'>浏览器会使用 HTTP Basic Authentication；公网入口必须通过 HTTPS 访问。</p>
            </main>",
            escape_html(workspace_name),
        ),
        0,
        false,
    )
}

pub fn error_page(workspace_name: &str, title: &str, message: &str) -> String {
    page(
        title,
        &format!(
            "<nav class='top-nav'>
                <a href='../../canvs' class='nav-back'>
                    <svg viewBox='0 0 24 24' width='16' height='16' stroke='currentColor' stroke-width='2' fill='none'><line x1='19' y1='12' x2='5' y2='12'></line><polyline points='12 19 5 12 12 5'></polyline></svg>
                    <span>返回任务看板</span>
                </a>
                <div class='nav-context'>
                    <span>工作区: </span>
                    <strong>{}</strong>
                </div>
            </nav>
            <main class='center-card'>
                <div class='empty-icon'>⚠️</div>
                <h1>{}</h1>
                <p>{}</p>
                <a href='../../canvs' class='btn-primary' style='display:inline-block;margin-top:16px;'>返回任务看板</a>
            </main>",
            escape_html(workspace_name),
            escape_html(title),
            escape_html(message),
        ),
        0,
        false,
    )
}

fn string_list(values: &[String], empty: &str, completed: bool) -> String {
    if values.is_empty() {
        return empty_line(empty);
    }
    format!(
        "<ol class='steps-list {}'>{}</ol>",
        if completed {
            "is-completed"
        } else {
            "is-pending"
        },
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                format!(
                    "<li class='step-item'>
                        <span class='step-indicator'>{}</span>
                        <p class='step-text'>{}</p>
                    </li>",
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

fn page(title: &str, body: &str, refresh_ms: u64, is_list_page: bool) -> String {
    let client_script = format!(
        r#"
        function copyText(text, btn) {{
            navigator.clipboard.writeText(text).then(() => {{
                const original = btn.innerHTML;
                btn.innerHTML = "<span style='color:var(--success);font-weight:600;'>已复制!</span>";
                setTimeout(() => {{ btn.innerHTML = original; }}, 1600);
            }}).catch(() => {{}});
        }}

        function formatRelativeTime(date) {{
            const now = new Date();
            const diffMs = now - date;
            const diffSec = Math.floor(diffMs / 1000);
            if (diffSec < 10) return '刚刚';
            if (diffSec < 60) return diffSec + ' 秒前';
            const diffMin = Math.floor(diffSec / 60);
            if (diffMin < 60) return diffMin + ' 分钟前';
            const diffHour = Math.floor(diffMin / 60);
            if (diffHour < 24) return diffHour + ' 小时前';
            const diffDay = Math.floor(diffHour / 24);
            if (diffDay < 7) return diffDay + ' 天前';
            return date.toLocaleDateString();
        }}

        document.querySelectorAll('time[data-time]').forEach((node) => {{
            const raw = node.dataset.time || '';
            let date;
            if (raw.startsWith('unix:')) date = new Date(Number(raw.slice(5)) * 1000);
            else if (/^\d{{13}}$/.test(raw)) date = new Date(Number(raw));
            else if (/^\d{{10}}$/.test(raw)) date = new Date(Number(raw) * 1000);
            else date = new Date(raw);
            if (!Number.isNaN(date.getTime())) {{
                node.title = date.toLocaleString();
                node.textContent = formatRelativeTime(date);
            }}
        }});

        {list_js}

        const scrollKey = 'anchor-canvs-scroll:' + location.pathname;
        addEventListener('beforeunload', () => sessionStorage.setItem(scrollKey, String(scrollY)));
        const savedScroll = sessionStorage.getItem(scrollKey);
        if (savedScroll) requestAnimationFrame(() => scrollTo(0, Number(savedScroll)));

        {refresh_code}
        "#,
        list_js = if is_list_page {
            r#"
            (function() {
                const searchInput = document.getElementById('task-search');
                const clearBtn = document.getElementById('clear-search');
                const filterTabs = document.getElementById('filter-tabs');
                const grid = document.getElementById('task-grid');
                const noMatch = document.getElementById('no-match');
                let currentFilter = 'all';

                function applyFilter() {
                    const query = (searchInput?.value || '').trim().toLowerCase();
                    if (clearBtn) clearBtn.style.display = query ? 'block' : 'none';
                    const cards = grid ? grid.querySelectorAll('.task-card') : [];
                    let visibleCount = 0;

                    cards.forEach(card => {
                        const status = card.dataset.status || '';
                        const category = card.dataset.category || '';
                        const isWorktree = card.dataset.worktree === 'true';
                        const searchData = card.dataset.search || '';

                        let matchesFilter = true;
                        if (currentFilter === 'active') matchesFilter = (category === 'active');
                        else if (currentFilter === 'verifying') matchesFilter = (category === 'verifying');
                        else if (currentFilter === 'completed') matchesFilter = (category === 'completed');
                        else if (currentFilter === 'worktree') matchesFilter = isWorktree;
                        else if (currentFilter === 'failed') matchesFilter = (category === 'failed');

                        const matchesSearch = !query || searchData.includes(query);
                        if (matchesFilter && matchesSearch) {
                            card.style.display = 'flex';
                            visibleCount++;
                        } else {
                            card.style.display = 'none';
                        }
                    });

                    if (noMatch) noMatch.style.display = (visibleCount === 0 && cards.length > 0) ? 'block' : 'none';
                }

                if (searchInput) {
                    searchInput.addEventListener('input', applyFilter);
                }
                if (clearBtn) {
                    clearBtn.addEventListener('click', () => {
                        searchInput.value = '';
                        applyFilter();
                        searchInput.focus();
                    });
                }
                if (filterTabs) {
                    filterTabs.addEventListener('click', (e) => {
                        const btn = e.target.closest('.tab');
                        if (!btn) return;
                        filterTabs.querySelectorAll('.tab').forEach(t => t.classList.remove('active'));
                        btn.classList.add('active');
                        currentFilter = btn.dataset.filter || 'all';
                        applyFilter();
                    });
                }
            })();
            "#
        } else {
            ""
        },
        refresh_code = if refresh_ms > 0 {
            format!(
                "setInterval(() => {{ if (!document.hidden) location.reload(); }}, {});",
                refresh_ms
            )
        } else {
            String::new()
        }
    );

    format!(
        "<!doctype html>
        <html lang='zh-CN'>
        <head>
            <meta charset='utf-8'>
            <meta name='viewport' content='width=device-width,initial-scale=1'>
            <meta name='color-scheme' content='light dark'>
            <title>{}</title>
            <style>{}</style>
        </head>
        <body>
            <div class='app-container'>
                {body}
            </div>
            <script>{}</script>
        </body>
        </html>",
        escape_html(title),
        STYLES,
        client_script,
    )
}

fn status_label(status: &str) -> String {
    match status {
        "active" => "进行中",
        "paused" => "已暂停",
        "verifying" => "验证中",
        "failed" => "失败",
        "incomplete" => "未完成终止",
        "completed" => "已完成",
        "completed_unverified" => "完成未验证",
        "rolled_back" => "已回滚",
        _ => "未知",
    }
    .into()
}

fn status_badge_class(status: &str) -> &'static str {
    match status {
        "active" => "badge-primary",
        "completed" | "completed_unverified" => "badge-success",
        "verifying" => "badge-warning",
        "failed" | "incomplete" => "badge-destructive",
        _ => "badge-secondary",
    }
}

fn outcome_badge_class(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "badge-success",
        Some(false) => "badge-destructive",
        None => "badge-secondary",
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
        values.push(format!("{files} 个文件"));
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
    format!("<p class='empty-line'>{}</p>", escape_html(message))
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
:root {
    --font-sans: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    --background: #f8fafc;
    --foreground: #0f172a;
    --card: #ffffff;
    --card-foreground: #0f172a;
    --card-border: #e2e8f0;
    --card-hover-border: #cbd5e1;
    --primary: #2563eb;
    --primary-foreground: #ffffff;
    --primary-subtle: #eff6ff;
    --primary-border: #bfdbfe;
    --secondary: #f1f5f9;
    --secondary-foreground: #334155;
    --muted: #f1f5f9;
    --muted-foreground: #64748b;
    --border: #e2e8f0;
    --input: #e2e8f0;
    --ring: #3b82f6;
    --success: #16a34a;
    --success-subtle: #f0fdf4;
    --success-border: #bbf7d0;
    --warning: #d97706;
    --warning-subtle: #fffbeb;
    --warning-border: #fde68a;
    --destructive: #dc2626;
    --destructive-subtle: #fef2f2;
    --destructive-border: #fecaca;
    --purple: #7c3aed;
    --purple-subtle: #f5f3ff;
    --purple-border: #ddd6fe;
    --radius-sm: 6px;
    --radius-md: 10px;
    --radius-lg: 16px;
    --shadow-sm: 0 1px 2px 0 rgb(0 0 0 / 0.05);
    --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.07), 0 2px 4px -2px rgb(0 0 0 / 0.05);
    --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.08), 0 4px 6px -4px rgb(0 0 0 / 0.05);
}

@media (prefers-color-scheme: dark) {
    :root {
        --background: #090d16;
        --foreground: #f8fafc;
        --card: #111827;
        --card-foreground: #f8fafc;
        --card-border: #1f293d;
        --card-hover-border: #334155;
        --primary: #3b82f6;
        --primary-foreground: #ffffff;
        --primary-subtle: #172554;
        --primary-border: #1e3a8a;
        --secondary: #1e293b;
        --secondary-foreground: #cbd5e1;
        --muted: #1e293b;
        --muted-foreground: #94a3b8;
        --border: #1e293b;
        --input: #1e293b;
        --ring: #3b82f6;
        --success: #22c55e;
        --success-subtle: #052e16;
        --success-border: #14532d;
        --warning: #f59e0b;
        --warning-subtle: #451a03;
        --warning-border: #78350f;
        --destructive: #ef4444;
        --destructive-subtle: #450a0a;
        --destructive-border: #7f1d1d;
        --purple: #a855f7;
        --purple-subtle: #2e1065;
        --purple-border: #581c87;
        --shadow-sm: 0 1px 2px 0 rgb(0 0 0 / 0.4);
        --shadow-md: 0 4px 6px -1px rgb(0 0 0 / 0.4);
        --shadow-lg: 0 10px 15px -3px rgb(0 0 0 / 0.5);
    }
}

* { box-sizing: border-box; margin: 0; padding: 0; }
body {
    font-family: var(--font-sans);
    background-color: var(--background);
    color: var(--foreground);
    line-height: 1.5;
    -webkit-font-smoothing: antialiased;
}
a { color: inherit; text-decoration: none; }

.app-container {
    max-width: 1240px;
    margin: 0 auto;
    padding: 24px 20px 48px;
}

/* Header & Hero */
.hero {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 24px;
    padding: 28px;
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-sm);
    margin-bottom: 24px;
}
.hero-main { flex: 1; min-width: 0; }
.brand-badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.12em;
    color: var(--primary);
    background: var(--primary-subtle);
    border: 1px solid var(--primary-border);
    padding: 3px 10px;
    border-radius: 9999px;
    margin-bottom: 12px;
}
.pulse-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--primary);
    box-shadow: 0 0 8px var(--primary);
}
.hero h1 {
    font-size: 28px;
    font-weight: 700;
    letter-spacing: -0.02em;
    margin-bottom: 8px;
}
.subtitle {
    font-size: 13px;
    color: var(--muted-foreground);
    line-height: 1.6;
    max-width: 720px;
}
.hero-stats {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    max-width: 380px;
    justify-content: flex-end;
}
.stat-pill {
    display: flex;
    flex-direction: column;
    padding: 8px 14px;
    background: var(--secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    min-width: 76px;
}
.stat-label { font-size: 11px; color: var(--muted-foreground); font-weight: 500; }
.stat-val { font-size: 18px; font-weight: 700; }
.stat-val.primary { color: var(--primary); }
.stat-val.warning { color: var(--warning); }
.stat-val.success { color: var(--success); }
.hero-time {
    width: 100%;
    text-align: right;
    font-size: 11px;
    color: var(--muted-foreground);
    margin-top: 4px;
}

/* Toolbar & Filters */
.toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 20px;
}
.search-box {
    position: relative;
    flex: 1;
    min-width: 260px;
    max-width: 420px;
}
.search-icon {
    position: absolute;
    left: 12px;
    top: 50%;
    transform: translateY(-50%);
    color: var(--muted-foreground);
    pointer-events: none;
}
.search-box input {
    width: 100%;
    padding: 9px 34px 9px 36px;
    font-size: 13px;
    background: var(--card);
    border: 1px solid var(--input);
    border-radius: var(--radius-md);
    color: var(--foreground);
    outline: none;
    transition: border-color 0.15s, box-shadow 0.15s;
}
.search-box input:focus {
    border-color: var(--ring);
    box-shadow: 0 0 0 2px var(--primary-subtle);
}
.clear-btn {
    position: absolute;
    right: 10px;
    top: 50%;
    transform: translateY(-50%);
    border: none;
    background: transparent;
    color: var(--muted-foreground);
    cursor: pointer;
    font-size: 12px;
}
.filter-tabs {
    display: flex;
    align-items: center;
    gap: 4px;
    background: var(--card);
    border: 1px solid var(--border);
    padding: 4px;
    border-radius: var(--radius-md);
    overflow-x: auto;
}
.tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    font-size: 12px;
    font-weight: 500;
    color: var(--muted-foreground);
    background: transparent;
    border: none;
    border-radius: var(--radius-sm);
    cursor: pointer;
    transition: all 0.15s;
    white-space: nowrap;
}
.tab:hover { color: var(--foreground); background: var(--secondary); }
.tab.active {
    color: var(--foreground);
    background: var(--secondary);
    font-weight: 600;
    box-shadow: var(--shadow-sm);
}
.tab-count {
    font-size: 10px;
    background: var(--muted);
    padding: 1px 6px;
    border-radius: 9999px;
}

/* Cards Grid */
.cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(350px, 1fr));
    gap: 18px;
}
.task-card {
    display: flex;
    flex-direction: column;
    justify-content: space-between;
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: var(--radius-lg);
    padding: 20px;
    box-shadow: var(--shadow-sm);
    transition: transform 0.2s cubic-bezier(0.16, 1, 0.3, 1), border-color 0.2s, box-shadow 0.2s;
    cursor: pointer;
}
.task-card:hover {
    transform: translateY(-3px);
    border-color: var(--primary);
    box-shadow: var(--shadow-md);
}
.card-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 12px;
}
.badges {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
}
.badge {
    display: inline-flex;
    align-items: center;
    font-size: 11px;
    font-weight: 600;
    padding: 2px 8px;
    border-radius: 9999px;
    border: 1px solid transparent;
}
.badge-primary { background: var(--primary-subtle); color: var(--primary); border-color: var(--primary-border); }
.badge-secondary { background: var(--secondary); color: var(--secondary-foreground); border-color: var(--border); }
.badge-muted { background: var(--muted); color: var(--muted-foreground); border-color: var(--border); }
.badge-success { background: var(--success-subtle); color: var(--success); border-color: var(--success-border); }
.badge-warning { background: var(--warning-subtle); color: var(--warning); border-color: var(--warning-border); }
.badge-destructive { background: var(--destructive-subtle); color: var(--destructive); border-color: var(--destructive-border); }
.badge-purple { background: var(--purple-subtle); color: var(--purple); border-color: var(--purple-border); }

.copy-id-btn {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 6px;
    background: var(--muted);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    color: var(--muted-foreground);
    cursor: pointer;
    font-size: 11px;
    transition: background 0.15s, color 0.15s;
}
.copy-id-btn:hover { background: var(--secondary); color: var(--foreground); }
.copy-id-btn code { font-family: var(--font-mono); }

.card-title {
    font-size: 14.5px;
    font-weight: 600;
    line-height: 1.55;
    color: var(--foreground);
    margin-bottom: 16px;
    display: -webkit-box;
    -webkit-line-clamp: 4;
    -webkit-box-orient: vertical;
    overflow: hidden;
}

/* Progress */
.progress-section {
    margin-top: auto;
    padding: 12px 0;
}
.progress-bar {
    height: 6px;
    background: var(--muted);
    border-radius: 9999px;
    overflow: hidden;
    margin-bottom: 6px;
}
.progress-fill {
    height: 100%;
    background: linear-gradient(90deg, var(--primary), #06b6d4);
    border-radius: inherit;
    transition: width 0.3s ease;
}
.progress-meta {
    display: flex;
    justify-content: space-between;
    font-size: 11.5px;
    color: var(--muted-foreground);
}
.progress-meta strong { color: var(--foreground); font-weight: 600; }

/* Card Footer */
.card-footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
    font-size: 11.5px;
    color: var(--muted-foreground);
}
.footer-meta .time-text {
    display: inline-flex;
    align-items: center;
    gap: 4px;
}
.footer-git .git-badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: var(--secondary);
    padding: 2px 7px;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
    font-size: 11px;
}
.branch-name { max-width: 130px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.commit-sha { color: var(--primary); font-weight: 600; }

/* Detail Page */
.top-nav {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 20px;
    font-size: 13px;
}
.nav-back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
    color: var(--primary);
    padding: 6px 12px;
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    transition: background 0.15s;
}
.nav-back:hover { background: var(--secondary); }
.nav-context { color: var(--muted-foreground); }
.nav-context strong { color: var(--foreground); }

.detail-hero {
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: var(--radius-lg);
    padding: 28px;
    box-shadow: var(--shadow-sm);
    margin-bottom: 24px;
}
.detail-header-top {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
}
.copy-id-btn.full {
    padding: 4px 10px;
    font-size: 12px;
}
.detail-title {
    font-size: 22px;
    font-weight: 700;
    line-height: 1.45;
    margin-bottom: 20px;
}
.detail-progress-card {
    background: var(--secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 16px;
    margin-bottom: 20px;
}
.detail-progress-head {
    display: flex;
    justify-content: space-between;
    font-size: 13px;
    margin-bottom: 10px;
}
.detail-progress-head .progress-label { font-weight: 600; }
.progress-bar.large { height: 10px; }
.detail-meta-strip {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 12px;
    padding-top: 16px;
    border-top: 1px solid var(--border);
}
.meta-item {
    display: flex;
    flex-direction: column;
    gap: 2px;
    font-size: 12px;
}
.meta-item .label { color: var(--muted-foreground); font-size: 11px; }
.value-pill, .sha-pill {
    display: inline-block;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--foreground);
}
.sha-pill {
    background: var(--muted);
    border: 1px solid var(--border);
    padding: 2px 6px;
    border-radius: var(--radius-sm);
    color: var(--primary);
    font-weight: 600;
}

/* Detail Grid */
.detail-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 20px;
}
.panel {
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: var(--radius-lg);
    padding: 20px;
    box-shadow: var(--shadow-sm);
}
.panel.wide { grid-column: 1 / -1; }
.panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 16px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
}
.panel-header h2 { font-size: 15px; font-weight: 600; }
.panel-count { font-size: 12px; color: var(--muted-foreground); }

/* Steps */
.steps-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 24px;
}
.steps-col-title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 12px;
}
.steps-col-title.completed { color: var(--success); }
.steps-col-title.pending { color: var(--muted-foreground); }
.steps-col-title .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
}
.steps-list {
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
}
.step-item {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 12px;
    background: var(--secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    font-size: 13px;
    line-height: 1.5;
}
.step-indicator {
    flex-shrink: 0;
    width: 20px;
    height: 20px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    font-size: 11px;
    font-weight: 700;
}
.is-completed .step-indicator {
    background: var(--success-subtle);
    color: var(--success);
    border: 1px solid var(--success-border);
}
.is-pending .step-indicator {
    background: var(--muted);
    color: var(--muted-foreground);
    border: 1px solid var(--border);
}
.step-text { flex: 1; margin: 0; }

/* Facts Grid */
.facts-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
}
.fact-card {
    background: var(--secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 12px;
}
.fact-card dt { font-size: 11px; color: var(--muted-foreground); margin-bottom: 4px; }
.fact-card dd { margin: 0; font-family: var(--font-mono); font-size: 12px; }

/* Timeline rows */
.panel-body {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 420px;
    overflow-y: auto;
    padding-right: 4px;
}
.timeline-row {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 14px;
    padding: 10px 12px;
    background: var(--secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    font-size: 12.5px;
}
.timeline-body { flex: 1; min-width: 0; }
.timeline-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 2px;
}
.op-tool { font-weight: 600; }
.op-kind { color: var(--muted-foreground); font-size: 11.5px; }
.timeline-time { font-size: 11px; color: var(--muted-foreground); }
.timeline-end {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
    flex-shrink: 0;
}
.timeline-meta { font-size: 11px; color: var(--muted-foreground); }
.command-code {
    display: block;
    font-family: var(--font-mono);
    font-size: 11.5px;
    background: var(--card);
    border: 1px solid var(--border);
    padding: 4px 8px;
    border-radius: var(--radius-sm);
    margin-bottom: 4px;
    white-space: pre-wrap;
    word-break: break-all;
}
.v-meta, .event-meta {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--muted-foreground);
}
.change-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
}
.files-summary { font-size: 11.5px; color: var(--muted-foreground); word-break: break-all; }
.empty-line {
    padding: 24px;
    text-align: center;
    font-size: 13px;
    color: var(--muted-foreground);
}

/* Empty Card */
.empty-card {
    grid-column: 1 / -1;
    padding: 48px 24px;
    text-align: center;
    background: var(--card);
    border: 1px dashed var(--border);
    border-radius: var(--radius-lg);
}
.empty-icon { font-size: 32px; margin-bottom: 12px; }
.empty-card h2 { font-size: 18px; margin-bottom: 6px; }
.empty-card p { font-size: 13px; color: var(--muted-foreground); }

.center-card {
    max-width: 520px;
    margin: 8vh auto 0;
    padding: 36px 28px;
    text-align: center;
    background: var(--card);
    border: 1px solid var(--card-border);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-md);
}
.center-card h1 { font-size: 24px; margin-bottom: 12px; }
.center-card p { font-size: 13.5px; color: var(--muted-foreground); line-height: 1.6; }
.btn-primary {
    display: inline-block;
    padding: 8px 18px;
    background: var(--primary);
    color: var(--primary-foreground);
    border-radius: var(--radius-md);
    font-weight: 600;
    font-size: 13px;
}

@media (max-width: 768px) {
    .hero { flex-direction: column; }
    .hero-stats { justify-content: flex-start; max-width: 100%; }
    .toolbar { flex-direction: column; align-items: stretch; }
    .search-box { max-width: 100%; }
    .cards { grid-template-columns: 1fr; }
    .detail-grid { grid-template-columns: 1fr; }
    .steps-grid { grid-template-columns: 1fr; }
    .facts-grid { grid-template-columns: 1fr; }
}
"#;

#[cfg(test)]
mod tests {
    use super::{escape_html, task_detail_page, task_list_page};
    use crate::canvs::{CanvsSnapshot, CanvsTask, CanvsTaskList};

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
                    workspace_mode: "shared".into(),
                    current: true,
                    active: true,
                    completed_steps: vec!["step 1".into()],
                    pending_steps: vec!["step 2".into()],
                    progress_percent: 50,
                    branch: Some("feature/canvs".into()),
                    head: Some("abc123456789".into()),
                    expected_head: Some("abc123456789".into()),
                    latest_change_id: None,
                    latest_verification_id: None,
                    created_at: "1".into(),
                    updated_at: "2".into(),
                    last_activity_at: Some("3".into()),
                }],
                refreshed_at: "2".into(),
            },
        );
        assert!(html.contains("href='./canvs/tasks/task-id'"));
        assert!(html.contains("活动任务"));
        assert!(html.contains("id='task-search'"));
        assert!(html.contains("data-filter='active'"));
        assert!(html.contains("feature/canvs"));
    }

    #[test]
    fn task_detail_renders_breadcrumb_and_panels() {
        let html = task_detail_page(
            "test-workspace",
            &CanvsSnapshot {
                workspace_id: "ws-1".into(),
                task: Some(CanvsTask {
                    id: "task-abc".into(),
                    objective: "重构 Canvs 看板".into(),
                    status: "completed".into(),
                    workspace_mode: "worktree".into(),
                    current: false,
                    active: false,
                    completed_steps: vec!["第一步".into(), "第二步".into()],
                    pending_steps: vec![],
                    progress_percent: 100,
                    branch: Some("main".into()),
                    head: Some("head-123".into()),
                    expected_head: Some("head-123".into()),
                    latest_change_id: Some("change-1".into()),
                    latest_verification_id: Some("verify-1".into()),
                    created_at: "2026-08-22T12:00:00Z".into(),
                    updated_at: "2026-08-22T12:30:00Z".into(),
                    last_activity_at: Some("2026-08-22T12:30:00Z".into()),
                }),
                recent_events: vec![],
                recent_operations: vec![],
                changes: vec![],
                verifications: vec![],
                refreshed_at: "2026-08-22T12:30:00Z".into(),
            },
        );
        assert!(html.contains("href='../../canvs'"));
        assert!(html.contains("重构 Canvs 看板"));
        assert!(html.contains("任务步骤分解"));
        assert!(html.contains("任务基线信息"));
        assert!(html.contains("Git Worktree"));
    }
}
