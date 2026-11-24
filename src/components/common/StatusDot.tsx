import { cn } from "../../lib/utils";

type StatusDotProps = {
  tone: "idle" | "live" | "busy";
  label: string;
};

export function StatusDot({ tone, label }: StatusDotProps) {
  return (
    <span className="inline-flex items-center gap-2 text-sm text-muted">
      <span
        className={cn("size-2 rounded-full", {
          "bg-line": tone === "idle",
          "bg-pine": tone === "live",
          "bg-ember": tone === "busy",
        })}
      />
      {label}
    </span>
  );
}
