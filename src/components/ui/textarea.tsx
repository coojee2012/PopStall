import type { TextareaHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Textarea({ className, ...props }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        "min-h-28 w-full resize-y rounded-2xl border border-line bg-paper px-4 py-3 text-sm leading-6 text-ink outline-none placeholder:text-muted focus:border-ember",
        className,
      )}
      {...props}
    />
  );
}
