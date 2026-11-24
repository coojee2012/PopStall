import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Card({ className, ...props }: HTMLAttributes<HTMLElement>) {
  return (
    <section
      className={cn("rounded-3xl border border-line bg-card px-6 py-6 shadow-[0_16px_40px_rgba(36,25,16,0.04)]", className)}
      {...props}
    />
  );
}
