import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

/**
 * Global viewport shell: one scroll owner, no stacked chrome leaks.
 * App.tsx follows this contract — Stage overlays as fixed inset-0.
 */
export function AppShell({
  header,
  sidebar,
  children,
  overlay,
  banner,
}: {
  header: ReactNode;
  sidebar?: ReactNode;
  children: ReactNode;
  overlay?: ReactNode;
  banner?: ReactNode;
}) {
  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-paper text-ink">
      <header className="relative z-10 shrink-0">{header}</header>
      {banner}
      <div className="flex min-h-0 flex-1 overflow-hidden">
        {sidebar ? (
          <aside className="plaza-scroll min-h-0 shrink-0 overflow-y-auto border-r border-line bg-sidebar">
            {sidebar}
          </aside>
        ) : null}
        <div className="min-h-0 min-w-0 flex-1 overflow-hidden">{children}</div>
      </div>
      {overlay}
    </div>
  );
}

/** Desk swap without nesting scroll parents. */
export function DeskPane({
  active,
  children,
  className,
}: {
  active: boolean;
  children: ReactNode;
  className?: string;
}) {
  if (!active) {
    return null;
  }
  return <div className={cn("flex h-full min-h-0 min-w-0 flex-col overflow-hidden", className)}>{children}</div>;
}
