import { useState } from "react";
import { Copy, Eye, EyeOff, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

export function SecretField({
  value,
  onChange,
  disabled,
  readOnly,
  placeholder,
  onRegenerate,
  regenerating,
  className,
}: {
  value: string;
  onChange?: (value: string) => void;
  disabled?: boolean;
  readOnly?: boolean;
  placeholder?: string;
  onRegenerate?: () => void | Promise<void>;
  regenerating?: boolean;
  className?: string;
}) {
  const [visible, setVisible] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      toast.success("已复制");
    } catch (error) {
      toast.error("复制失败", { description: String(error) });
    }
  };

  return (
    <div className={cn("flex gap-2", className)}>
      <Input
        type={visible ? "text" : "password"}
        value={value}
        disabled={disabled}
        readOnly={readOnly}
        placeholder={placeholder}
        className="min-w-0 flex-1 font-mono text-xs"
        onChange={(event) => onChange?.(event.target.value)}
      />
      <Button type="button" variant="outline" size="icon" aria-label={visible ? "隐藏密钥" : "显示密钥"} onClick={() => setVisible((current) => !current)}>
        {visible ? <EyeOff /> : <Eye />}
      </Button>
      <Button type="button" variant="outline" size="icon" disabled={!value} aria-label="复制密钥" onClick={() => void copy()}>
        <Copy />
      </Button>
      {onRegenerate && (
        <Button type="button" variant="outline" size="icon" disabled={disabled || regenerating} aria-label="重新生成密钥" onClick={() => void onRegenerate()}>
          <RefreshCw className={regenerating ? "animate-spin" : undefined} />
        </Button>
      )}
    </div>
  );
}
