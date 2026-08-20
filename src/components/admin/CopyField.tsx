import { Check, Copy } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function CopyField({ label, value, hint, loading = false }: { label: string; value: string; hint?: string; loading?: boolean }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    if (!value) return;
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch (error) {
      toast.error("复制失败", { description: String(error) });
    }
  };

  return (
    <div className="grid gap-1.5">
      <div className="flex items-center justify-between gap-3">
        <span className="text-xs font-medium text-muted-foreground">{label}</span>
        {hint && <span className="text-[11px] text-muted-foreground">{hint}</span>}
      </div>
      <div className="flex gap-2">
        <Input readOnly value={loading ? "加载中…" : value} className="min-w-0 flex-1 font-mono text-xs" />
        <Button type="button" variant="outline" size="icon" disabled={loading || !value} aria-label={`复制${label}`} onClick={() => void copy()}>
          {copied ? <Check /> : <Copy />}
        </Button>
      </div>
    </div>
  );
}
