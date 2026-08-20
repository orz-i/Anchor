import { Check, ChevronDown, Copy, History } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";

export const CHATGPT_SESSION_PROMPT = `请使用当前工作区的 Anchor MCP 初始化本 ChatGPT 会话对应的独立开发 Session。
在回答本会话的第一个用户请求前，先且仅调用一次 session，operation=open；即使用户没有明确要求恢复，也必须执行。
Session 默认自然隔离：open 不会读取、摘要或注入其他 Session 的内容，也不要主动调用 list/get 恢复历史。
只有当用户明确要求恢复、查找或引用之前的工作时，才调用 session operation=list 获取有限元数据，再对明确相关的 session_id 调用 operation=get。
旧 docs/history-session/ 是冻结归档，不迁移、不参与新 Session；仅在用户明确要求旧归档内容时才用 read_file 精确读取。
不要在同一 ChatGPT 会话中重复创建 Session。保存 open 返回的 session_id 和 session_path；后续 checkpoint 原样使用 session_id，并将 session_path 作为 expected_path。
插件会在受支持的代码变更、提交、命令阶段和浏览器证据阶段同步写入幂等里程碑检查点，但这不能替代最终交接。
每个用户任务完成后、发送最终答复前调用 session operation=checkpoint，记录已脱敏的结论、决策、文件变更、验证结果、遗留问题和下一步。
只有最终 checkpoint 返回 ok=true，且返回的 session_id、path 和 expected_path 仍指向同一 Session 时，才能说明最终进度已保存。`;

export function ChatGptSessionPrompt() {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(CHATGPT_SESSION_PROMPT);
      setCopied(true);
      toast.success("新会话启动提示词已复制");
      window.setTimeout(() => setCopied(false), 1800);
    } catch (error) {
      toast.error("无法复制提示词", { description: String(error) });
    }
  };

  return (
    <Card aria-labelledby="chatgpt-session-prompt-title">
      <CardContent className="p-4">
        <div className="flex flex-wrap items-center justify-between gap-4">
          <div className="flex min-w-0 items-center gap-3">
            <span className="grid size-9 shrink-0 place-items-center rounded-lg bg-primary/10 text-primary"><History className="size-4" /></span>
            <div className="min-w-0">
              <h3 id="chatgpt-session-prompt-title" className="text-sm font-medium">ChatGPT 新会话启动提示词</h3>
              <p className="mt-1 text-xs text-muted-foreground">每个新聊天创建独立 Session；仅在明确需要时按索引读取历史。</p>
            </div>
          </div>
          <div className="flex gap-2">
            <Button type="button" size="sm" className="min-h-11" onClick={() => void copy()}>{copied ? <Check data-icon="inline-start" /> : <Copy data-icon="inline-start" />}{copied ? "已复制" : "复制完整提示词"}</Button>
            <Button type="button" size="sm" className="min-h-11" variant="outline" aria-expanded={expanded} onClick={() => setExpanded((current) => !current)}>
              {expanded ? "收起提示词" : "查看完整提示词"}<ChevronDown data-icon="inline-end" className={expanded ? "rotate-180 transition-transform" : "transition-transform"} />
            </Button>
          </div>
        </div>
        {expanded && <pre className="mt-4 whitespace-pre-wrap break-words rounded-xl border bg-muted/40 p-3 font-mono text-xs leading-5">{CHATGPT_SESSION_PROMPT}</pre>}
        <span className="sr-only" aria-live="polite">{copied ? "提示词已复制" : ""}</span>
      </CardContent>
    </Card>
  );
}
