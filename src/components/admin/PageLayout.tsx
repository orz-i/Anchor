import type { ReactNode } from "react";

export function PageLayout({
  kicker,
  title,
  description,
  actions,
  children,
}: {
  kicker: ReactNode;
  title: string;
  description?: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="flex h-full min-h-0 flex-col overflow-hidden">
      <header className="shrink-0 border-b bg-background/80 px-7 py-5 backdrop-blur">
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <p className="text-[11px] font-semibold uppercase tracking-[0.16em] text-muted-foreground">{kicker}</p>
            <h2 className="mt-1 text-2xl font-semibold tracking-tight">{title}</h2>
            {description && <p className="mt-2 max-w-3xl text-sm leading-6 text-muted-foreground">{description}</p>}
          </div>
          {actions}
        </div>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-7 py-6">{children}</div>
    </section>
  );
}
